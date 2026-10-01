//! A declaration signature as classified tokens, not as a string.
//!
//! Surfaces need to colour a signature, wrap it, and link the types inside it.
//! Doing that from a `String` means every surface re-writes the same lexer and
//! they drift. So the model owns one lexical tokenizer per language family and
//! hands every surface the same [`Token`] stream.
//!
//! The tokenizer is **lexical only**. It knows each language's keyword set and
//! its punctuation, and it uses position — what follows `->`, what precedes
//! `:`, what sits inside `<…>` — to tell a type from a binding. It never
//! consults an index, so a [`TokenKind::Type`] means "this reads as a type
//! here", not "this is proven to be that type". When a surface can match a
//! type token to a name in the same project it attaches a [`Target`] marked
//! [`Resolved::ByName`]; that marker exists precisely so nobody later mistakes
//! a name match for a semantic link.

use crate::identity::{Coordinate, IdentityKey};
use crate::language::Language;
use core::fmt;

/// What one signature token reads as.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TokenKind {
    /// A reserved word of the language.
    Keyword,
    /// The declared name of the declaration itself.
    Name,
    /// An identifier in type position.
    Type,
    /// A parameter or field name being bound.
    Binding,
    /// A Rust lifetime or a language's equivalent sigil-prefixed marker.
    Lifetime,
    /// Structural punctuation.
    Punctuation,
    /// A numeric, string, or character literal.
    Literal,
    /// Anything the lexer classified as ordinary text.
    Text,
}

impl TokenKind {
    /// Returns the stable lowercase name shared by every surface.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Keyword => "keyword",
            Self::Name => "name",
            Self::Type => "type",
            Self::Binding => "binding",
            Self::Lifetime => "lifetime",
            Self::Punctuation => "punctuation",
            Self::Literal => "literal",
            Self::Text => "text",
        }
    }
}

/// How confident the model is that a type token points at a declaration.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Resolved {
    /// The token text matched a declaration name in the same project.
    ///
    /// This is a spelling match, not a proven semantic link, and every surface
    /// must render it as such.
    ByName,
}

/// A declaration a type token may refer to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    coordinate: Coordinate,
    key: IdentityKey,
    resolved: Resolved,
}

impl Target {
    /// Attaches an unproven, name-matched target to a type token.
    #[must_use]
    pub const fn by_name(coordinate: Coordinate, key: IdentityKey) -> Self {
        Self {
            coordinate,
            key,
            resolved: Resolved::ByName,
        }
    }

    /// Returns the exact coordinate the target can be read at.
    #[must_use]
    pub const fn coordinate(&self) -> &Coordinate {
        &self.coordinate
    }

    /// Returns the target's stable key.
    #[must_use]
    pub const fn key(&self) -> IdentityKey {
        self.key
    }

    /// Returns how the target was matched.
    #[must_use]
    pub const fn resolved(&self) -> Resolved {
        self.resolved
    }
}

/// One classified lexeme of a signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    text: String,
    kind: TokenKind,
    target: Option<Target>,
}

impl Token {
    /// Classifies one lexeme.
    #[must_use]
    pub fn new(text: impl Into<String>, kind: TokenKind) -> Self {
        Self {
            text: text.into(),
            kind,
            target: None,
        }
    }

    /// Returns the lexeme text exactly as it appeared.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns what the lexeme reads as.
    #[must_use]
    pub const fn kind(&self) -> TokenKind {
        self.kind
    }

    /// Returns the unproven declaration this type token may refer to.
    #[must_use]
    pub const fn target(&self) -> Option<&Target> {
        self.target.as_ref()
    }

    /// Attaches an unproven, name-matched target.
    #[must_use]
    pub fn with_target(mut self, target: Target) -> Self {
        if self.kind == TokenKind::Type {
            self.target = Some(target);
        }
        self
    }
}

/// One declaration signature as an ordered token stream.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Signature {
    tokens: Box<[Token]>,
}

impl Signature {
    /// Tokenizes one signature under the rules of its source language.
    #[must_use]
    pub fn tokenize(text: &str, language: Language) -> Self {
        Self {
            tokens: classify(&lex(text), language),
        }
    }

    /// Returns the classified tokens in source order.
    #[must_use]
    pub fn tokens(&self) -> &[Token] {
        &self.tokens
    }

    /// Returns whether this signature carries no token.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Returns the signature text, reassembled exactly.
    #[must_use]
    pub fn text(&self) -> String {
        self.tokens.iter().map(Token::text).collect()
    }

    /// Attaches unproven, name-matched targets supplied by a resolver.
    #[must_use]
    pub fn resolve_types(
        self,
        mut resolver: impl FnMut(&str) -> Option<(Coordinate, IdentityKey)>,
    ) -> Self {
        Self {
            tokens: self
                .tokens
                .into_vec()
                .into_iter()
                .map(|token| {
                    if token.kind == TokenKind::Type
                        && let Some((coordinate, key)) = resolver(token.text())
                    {
                        return token.with_target(Target::by_name(coordinate, key));
                    }
                    token
                })
                .collect(),
        }
    }
}

impl fmt::Display for Signature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text())
    }
}

/// One raw lexeme before language-aware classification.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Lexeme {
    text: String,
    class: LexClass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LexClass {
    Identifier,
    Number,
    Quoted,
    Lifetime,
    Punctuation,
    Space,
}

fn lex(text: &str) -> Vec<Lexeme> {
    let mut lexemes = Vec::new();
    let mut characters = text.chars().peekable();
    while let Some(first) = characters.next() {
        let (class, mut buffer) = (start_class(first), String::from(first));
        match class {
            LexClass::Identifier | LexClass::Number | LexClass::Space => {
                while characters
                    .peek()
                    .copied()
                    .is_some_and(|next| continues(class, next))
                {
                    buffer.extend(characters.next());
                }
            }
            LexClass::Quoted => {
                take_quoted(&mut characters, &mut buffer, first);
            }
            LexClass::Lifetime => {
                while characters
                    .peek()
                    .copied()
                    .is_some_and(|next| next.is_alphanumeric() || next == '_')
                {
                    buffer.extend(characters.next());
                }
                if buffer.chars().count() == 1 {
                    take_quoted(&mut characters, &mut buffer, first);
                }
            }
            LexClass::Punctuation => {}
        }
        lexemes.push(Lexeme {
            text: buffer,
            class,
        });
    }
    lexemes
}

fn take_quoted(
    characters: &mut core::iter::Peekable<core::str::Chars<'_>>,
    buffer: &mut String,
    quote: char,
) {
    let mut escaped = false;
    for next in characters.by_ref() {
        buffer.push(next);
        if escaped {
            escaped = false;
        } else if next == '\\' {
            escaped = true;
        } else if next == quote {
            break;
        }
    }
}

fn start_class(character: char) -> LexClass {
    if character.is_whitespace() {
        LexClass::Space
    } else if character.is_ascii_digit() {
        LexClass::Number
    } else if character == '"' || character == '`' {
        LexClass::Quoted
    } else if character == '\'' {
        LexClass::Lifetime
    } else if character.is_alphabetic() || character == '_' || character == '$' || character == '@' {
        LexClass::Identifier
    } else {
        LexClass::Punctuation
    }
}

fn continues(class: LexClass, character: char) -> bool {
    match class {
        LexClass::Identifier => character.is_alphanumeric() || character == '_' || character == '$',
        LexClass::Number => character.is_alphanumeric() || character == '.' || character == '_',
        LexClass::Space => character.is_whitespace(),
        LexClass::Quoted | LexClass::Lifetime | LexClass::Punctuation => false,
    }
}

/// Tracks just enough position to tell a type from a binding.
#[derive(Clone, Copy, Debug, Default)]
struct Cursor {
    generics: u32,
    parameters: u32,
    after_declarator: bool,
    after_type_marker: bool,
    named: bool,
}

fn classify(lexemes: &[Lexeme], language: Language) -> Box<[Token]> {
    let mut cursor = Cursor::default();
    let mut tokens = Vec::with_capacity(lexemes.len());
    for (index, lexeme) in lexemes.iter().enumerate() {
        let kind = match lexeme.class {
            LexClass::Space => TokenKind::Text,
            LexClass::Number | LexClass::Quoted => TokenKind::Literal,
            LexClass::Lifetime => lifetime_kind(language),
            LexClass::Punctuation => {
                update_punctuation(&mut cursor, &lexeme.text, previous_significant(lexemes, index));
                TokenKind::Punctuation
            }
            LexClass::Identifier => identifier_kind(&mut cursor, lexemes, index, language),
        };
        tokens.push(Token::new(lexeme.text.clone(), kind));
    }
    tokens.into_boxed_slice()
}

const fn lifetime_kind(language: Language) -> TokenKind {
    match language {
        Language::Rust => TokenKind::Lifetime,
        _ => TokenKind::Literal,
    }
}

fn update_punctuation(cursor: &mut Cursor, text: &str, previous: Option<&str>) {
    match text {
        "<" | "[" => cursor.generics = cursor.generics.saturating_add(1),
        ">" if previous == Some("-") => cursor.after_type_marker = true,
        ">" | "]" => cursor.generics = cursor.generics.saturating_sub(1),
        ":" => cursor.after_type_marker = true,
        "(" => {
            cursor.parameters = cursor.parameters.saturating_add(1);
            cursor.after_type_marker = false;
        }
        ")" => {
            cursor.parameters = cursor.parameters.saturating_sub(1);
            cursor.after_type_marker = false;
        }
        "," | "{" | ";" => cursor.after_type_marker = false,
        _ => {}
    }
}

fn identifier_kind(
    cursor: &mut Cursor,
    lexemes: &[Lexeme],
    index: usize,
    language: Language,
) -> TokenKind {
    let text = lexemes.get(index).map_or("", |lexeme| lexeme.text.as_str());
    if is_keyword(text, language) {
        cursor.after_declarator = is_declarator(text);
        cursor.after_type_marker = false;
        return TokenKind::Keyword;
    }
    if is_primitive_type(text, language) {
        cursor.after_type_marker = false;
        return TokenKind::Type;
    }
    let next = next_significant(lexemes, index);
    if cursor.after_declarator && !cursor.named {
        cursor.after_declarator = false;
        cursor.named = true;
        return TokenKind::Name;
    }
    if cursor.after_type_marker || cursor.generics > 0 {
        cursor.after_type_marker = false;
        return TokenKind::Type;
    }
    if next == Some(":") {
        return TokenKind::Binding;
    }
    if !cursor.named && cursor.parameters == 0 && next == Some("(") {
        cursor.named = true;
        return TokenKind::Name;
    }
    positional_kind(cursor, lexemes, index, language)
}

/// Classifies a bare identifier from its neighbours, where the language spells
/// a parameter as two adjacent identifiers rather than `name: Type`.
fn positional_kind(
    cursor: &Cursor,
    lexemes: &[Lexeme],
    index: usize,
    language: Language,
) -> TokenKind {
    let next = next_significant(lexemes, index);
    let previous = previous_significant(lexemes, index);
    let next_opens_name = next.is_some_and(is_identifier_like);
    let next_opens_type = next.is_some_and(|text| is_identifier_like(text) || opens_type(text));
    let follows_type = previous.is_some_and(|text| is_identifier_like(text) || closes_type(text));
    match language {
        Language::Java | Language::CSharp | Language::C | Language::Cxx if cursor.parameters > 0 => {
            if next_opens_name {
                TokenKind::Type
            } else if follows_type {
                TokenKind::Binding
            } else {
                TokenKind::Text
            }
        }
        Language::Go if cursor.parameters > 0 => {
            if next_opens_type {
                TokenKind::Binding
            } else if follows_type {
                TokenKind::Type
            } else {
                TokenKind::Text
            }
        }
        _ if language.uses_scope_resolution() && starts_uppercase(text_at(lexemes, index)) => {
            TokenKind::Type
        }
        _ => TokenKind::Text,
    }
}

fn text_at(lexemes: &[Lexeme], index: usize) -> &str {
    lexemes.get(index).map_or("", |lexeme| lexeme.text.as_str())
}

/// Returns whether a lexeme ends a type expression, so an identifier after it
/// is the thing being bound rather than the type of it.
fn closes_type(text: &str) -> bool {
    matches!(text, "*" | "&" | ">" | "]" | "?")
}

/// Returns whether a lexeme opens a type expression, so an identifier before it
/// is the thing being bound rather than the type of it.
fn opens_type(text: &str) -> bool {
    matches!(text, "*" | "[" | "." | "<")
}

fn is_identifier_like(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|first| first.is_alphabetic() || first == '_')
}

fn starts_uppercase(text: &str) -> bool {
    text.chars().next().is_some_and(char::is_uppercase)
}

fn next_significant(lexemes: &[Lexeme], index: usize) -> Option<&str> {
    lexemes
        .iter()
        .skip(index.saturating_add(1))
        .find(|lexeme| lexeme.class != LexClass::Space)
        .map(|lexeme| lexeme.text.as_str())
}

fn previous_significant(lexemes: &[Lexeme], index: usize) -> Option<&str> {
    lexemes
        .iter()
        .take(index)
        .rev()
        .find(|lexeme| lexeme.class != LexClass::Space)
        .map(|lexeme| lexeme.text.as_str())
}

fn is_declarator(text: &str) -> bool {
    matches!(
        text,
        "fn" | "func"
            | "function"
            | "def"
            | "struct"
            | "enum"
            | "trait"
            | "union"
            | "type"
            | "class"
            | "interface"
            | "record"
            | "namespace"
            | "module"
            | "impl"
    )
}

fn is_keyword(text: &str, language: Language) -> bool {
    keywords(language).contains(&text) || SHARED.contains(&text)
}

/// Returns whether one identifier is a built-in type name of its language.
///
/// These read as types, not as reserved words: `string` in `lane: string` is
/// the answer to "what type is this?", and colouring it like `function` would
/// hide exactly the information a reader is looking for.
fn is_primitive_type(text: &str, language: Language) -> bool {
    SHARED_TYPES.contains(&text) || primitive_types(language).contains(&text)
}

const SHARED_TYPES: &[&str] = &["void", "bool", "char", "double", "float", "int", "long", "short"];

const fn primitive_types(language: Language) -> &'static [&'static str] {
    match language {
        Language::Rust => RUST_TYPES,
        Language::TypeScript => TYPESCRIPT_TYPES,
        Language::Python => PYTHON_TYPES,
        Language::Go => GO_TYPES,
        Language::Java => JAVA_TYPES,
        Language::CSharp => CSHARP_TYPES,
        Language::C | Language::Cxx => C_TYPES,
        Language::Unknown => &[],
    }
}

const RUST_TYPES: &[&str] = &[
    "bool", "char", "f32", "f64", "i8", "i16", "i32", "i64", "i128", "isize", "str", "u8", "u16",
    "u32", "u64", "u128", "usize",
];

const TYPESCRIPT_TYPES: &[&str] = &[
    "any", "bigint", "boolean", "never", "number", "object", "string", "symbol", "undefined",
    "unknown", "void",
];

const PYTHON_TYPES: &[&str] = &[
    "bool", "bytes", "complex", "dict", "float", "frozenset", "int", "list", "set", "str", "tuple",
];

const GO_TYPES: &[&str] = &[
    "any", "bool", "byte", "complex64", "complex128", "error", "float32", "float64", "int",
    "int8", "int16", "int32", "int64", "rune", "string", "uint", "uint8", "uint16", "uint32",
    "uint64", "uintptr",
];

const JAVA_TYPES: &[&str] = &[
    "boolean", "byte", "char", "double", "float", "int", "long", "short", "void",
];

const CSHARP_TYPES: &[&str] = &[
    "bool", "byte", "char", "decimal", "double", "float", "int", "long", "nint", "nuint",
    "object", "sbyte", "short", "string", "uint", "ulong", "ushort", "void",
];

const C_TYPES: &[&str] = &[
    "bool", "char", "double", "float", "int", "long", "short", "signed", "size_t", "unsigned",
    "void", "wchar_t",
];

/// Keywords every supported language shares.
const SHARED: &[&str] = &[
    "class", "const", "else", "enum", "extern", "false", "for", "if", "import", "in", "new", "null", "return", "static", "struct", "switch", "true", "type", "while",
];

const fn keywords(language: Language) -> &'static [&'static str] {
    match language {
        Language::Rust => RUST,
        Language::TypeScript => TYPESCRIPT,
        Language::Python => PYTHON,
        Language::Go => GO,
        Language::Java => JAVA,
        Language::CSharp => CSHARP,
        Language::C => C,
        Language::Cxx => CXX,
        Language::Unknown => &[],
    }
}

const RUST: &[&str] = &[
    "as", "async", "await", "crate", "dyn", "fn", "impl", "let", "mod", "move", "mut", "pub",
    "ref", "self", "Self", "super", "trait", "union", "unsafe", "use", "where", "loop", "match",
];

const TYPESCRIPT: &[&str] = &[
    "abstract", "as", "async", "await", "declare", "export", "extends", "function", "implements", "interface", "keyof", "let", "namespace", "private", "protected", "public", "readonly", "this", "typeof", "var", "yield",
];

const PYTHON: &[&str] = &[
    "and", "async", "await", "def", "elif", "except", "finally", "from", "global", "is", "lambda",
    "None", "nonlocal", "not", "or", "pass", "raise", "self", "try", "with", "yield",
];

const GO: &[&str] = &[
    "chan", "defer", "func", "go", "interface", "map", "package", "range", "select", "var",
];

const JAVA: &[&str] = &[
    "abstract", "extends", "final", "implements", "interface", "native", "package", "private", "protected", "public", "record", "sealed", "synchronized", "throws", "transient", "volatile",
];

const CSHARP: &[&str] = &[
    "abstract", "async", "await", "delegate", "event", "internal", "interface", "namespace", "override", "partial", "private", "protected", "public", "readonly", "record", "sealed", "using", "var", "virtual", "where",
];

const C: &[&str] = &[
    "inline", "register", "restrict", "sizeof", "typedef", "union", "volatile",
];

const CXX: &[&str] = &[
    "auto", "concept", "constexpr", "decltype", "explicit", "friend", "inline", "mutable", "namespace", "noexcept", "operator", "private", "protected", "public", "requires", "sizeof", "template", "this", "typedef", "typename", "union", "using", "virtual", "volatile",
];
