//! SQL written as string literals in application code (TypeScript, JavaScript
//! and Python), found lexically and handed to the same SQL worker as `.sql`
//! files.
//!
//! The scan is a bounded lexer, never a parser of the host language: it walks
//! tokens (skipping comments, strings, template literals and regular
//! expressions), finds method calls such as `db.prepare(...)`,
//! `client.query(...)` or `cursor.execute(...)`, reads the first argument as a
//! concatenation of literals, and keeps only arguments whose static text starts
//! like SQL. Everything found in one file is composed into one SQL document,
//! one statement per call, and that document is the file's *embedded host*: its
//! virtual path is the real path plus [`VIRTUAL_SUFFIX`], and its bytes are
//! re-derivable from the real file, so the host is verified exactly like a
//! `.sql` file (the composition is deterministic).
//!
//! A literal made only of static text is *static*: the worker analyses its
//! exact text. A literal that is built (template interpolation, concatenation
//! with a variable, `%`-formatting, f-strings) is *dynamic*: each dynamic piece
//! becomes the placeholder identifier [`DYNAMIC_PLACEHOLDER`] and every
//! reference found in the statement is a candidate, never exact. SQL assembled
//! somewhere else and passed in as a bare variable is not seen at all: that
//! stays a stated gap.
use backend_library::sql_workspace::SqlDialect;

/// Appended to an application file's path to name its embedded host.
pub(super) const VIRTUAL_SUFFIX: &str = "#embedded.sql";
/// Stands for any part of a statement that is only known at run time.
pub(super) const DYNAMIC_PLACEHOLDER: &str = "nudox_dynamic";
/// Application sources larger than this are not scanned (reported as a gap).
pub(super) const MAX_SCAN_BYTES: usize = 1024 * 1024;
/// Statements kept per file; further calls are dropped and counted.
pub(super) const MAX_FRAGMENTS: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Language {
    Script,
    Python,
}

impl Language {
    pub(super) fn of(path: &str) -> Option<Self> {
        let (_, extension) = path.rsplit_once('.')?;
        match extension.to_ascii_lowercase().as_str() {
            "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Some(Self::Script),
            "py" | "pyw" => Some(Self::Python),
            _ => None,
        }
    }
}

/// Whether `path` can hold embedded SQL at all.
pub(super) fn is_candidate(path: &str) -> bool {
    !path.ends_with(VIRTUAL_SUFFIX) && Language::of(path).is_some()
}

/// The application file an embedded host path names.
pub(super) fn real_path(virtual_path: &str) -> Option<&str> {
    virtual_path.strip_suffix(VIRTUAL_SUFFIX).filter(|path| is_candidate(path))
}

pub(super) fn virtual_path(real: &str) -> String {
    format!("{real}{VIRTUAL_SUFFIX}")
}

/// One query call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Fragment {
    /// Byte range of the argument expression in the application file.
    pub(super) start: usize,
    pub(super) end: usize,
    /// One-based line of `start`.
    pub(super) line: u32,
    /// Built from non-literal parts: every reference it makes is a candidate.
    pub(super) dynamic: bool,
    /// Byte range of this statement's text in the composed document.
    pub(super) text_start: usize,
    pub(super) text_end: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Scan {
    /// The embedded host's bytes: one statement per call, each ended by `;\n` (`\n;\n` after a line comment).
    pub(super) composed: Vec<u8>,
    pub(super) fragments: Vec<Fragment>,
    /// Dialect the file's imports name, when they name one.
    pub(super) dialect: Option<SqlDialect>,
    /// Calls found beyond [`MAX_FRAGMENTS`] and left out.
    pub(super) dropped: usize,
}

impl Scan {
    /// The call whose statement text contains `offset` of the composed document.
    pub(super) fn fragment_at(&self, offset: usize) -> Option<&Fragment> {
        self.fragments.iter().find(|fragment| offset >= fragment.text_start && offset < fragment.text_end.saturating_add(1))
    }
}

#[derive(Clone, Debug)]
enum Piece {
    Text(String),
    Dynamic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Ident,
    Punct,
    Str,
    Other,
}

#[derive(Clone, Debug)]
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
    /// For `Str`: the literal's pieces. A byte string has none and is not SQL.
    pieces: Option<Vec<Piece>>,
}

struct Lexer<'a> {
    bytes: &'a [u8],
    text: &'a str,
    language: Language,
    position: usize,
    tokens: Vec<Token>,
}

fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$' || byte >= 0x80
}

fn is_ident_part(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str, language: Language) -> Self {
        Self { bytes: text.as_bytes(), text, language, position: 0, tokens: Vec::new() }
    }

    fn peek(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.position + ahead).copied()
    }

    /// A `/` starts a regular expression unless the previous token ends an operand.
    fn regex_allowed(&self) -> bool {
        match self.tokens.last() {
            None => true,
            Some(token) => match token.kind {
                Kind::Str | Kind::Other => false,
                Kind::Ident => {
                    matches!(&self.text[token.start..token.end], "return" | "typeof" | "case" | "in" | "of" | "delete" | "void" | "throw" | "new" | "else" | "do" | "yield" | "await")
                }
                Kind::Punct => !matches!(&self.text[token.start..token.end], ")" | "]" | "}"),
            },
        }
    }

    fn run(mut self) -> Vec<Token> {
        while let Some(byte) = self.peek(0) {
            match byte {
                b' ' | b'\t' | b'\r' | b'\n' | 0x0c => self.position += 1,
                b'/' if self.language == Language::Script && self.peek(1) == Some(b'/') => self.skip_line(),
                b'/' if self.language == Language::Script && self.peek(1) == Some(b'*') => self.skip_block(),
                b'#' if self.language == Language::Python => self.skip_line(),
                b'/' if self.language == Language::Script && self.regex_allowed() => self.regex(),
                b'\'' | b'"' => self.string(self.position, byte, false, false),
                b'`' if self.language == Language::Script => self.template(),
                b'0'..=b'9' => self.number(),
                _ if is_ident_start(byte) => self.ident(),
                _ => {
                    let start = self.position;
                    self.position += 1;
                    // Multi-byte punctuation never matters here; keep one byte.
                    self.tokens.push(Token { kind: Kind::Punct, start, end: self.position, pieces: None });
                }
            }
        }
        self.tokens
    }

    fn skip_line(&mut self) {
        while let Some(byte) = self.peek(0) {
            if byte == b'\n' {
                break;
            }
            self.position += 1;
        }
    }

    fn skip_block(&mut self) {
        self.position += 2;
        while self.position < self.bytes.len() {
            if self.peek(0) == Some(b'*') && self.peek(1) == Some(b'/') {
                self.position += 2;
                return;
            }
            self.position += 1;
        }
    }

    fn regex(&mut self) {
        let start = self.position;
        self.position += 1;
        let mut in_class = false;
        while let Some(byte) = self.peek(0) {
            match byte {
                b'\\' => self.position += 2,
                b'\n' => break,
                b'[' => {
                    in_class = true;
                    self.position += 1;
                }
                b']' => {
                    in_class = false;
                    self.position += 1;
                }
                b'/' if !in_class => {
                    self.position += 1;
                    while self.peek(0).is_some_and(|b| b.is_ascii_alphabetic()) {
                        self.position += 1;
                    }
                    break;
                }
                _ => self.position += 1,
            }
        }
        self.position = self.position.min(self.bytes.len());
        self.tokens.push(Token { kind: Kind::Other, start, end: self.position, pieces: None });
    }

    fn number(&mut self) {
        let start = self.position;
        while self.peek(0).is_some_and(|b| is_ident_part(b) || b == b'.') {
            self.position += 1;
        }
        self.tokens.push(Token { kind: Kind::Other, start, end: self.position, pieces: None });
    }

    fn ident(&mut self) {
        let start = self.position;
        while self.peek(0).is_some_and(is_ident_part) {
            self.position += 1;
        }
        let word = &self.text[start..self.position];
        if self.language == Language::Python && matches!(self.peek(0), Some(b'\'' | b'"')) {
            let lower = word.to_ascii_lowercase();
            if lower.len() <= 2 && lower.bytes().all(|b| matches!(b, b'r' | b'b' | b'u' | b'f')) {
                let raw = lower.contains('r');
                let bytes = lower.contains('b');
                let formatted = lower.contains('f');
                let quote = self.bytes[self.position];
                self.string_python(start, quote, raw, bytes, formatted);
                return;
            }
        }
        self.tokens.push(Token { kind: Kind::Ident, start, end: self.position, pieces: None });
    }

    fn string(&mut self, start: usize, quote: u8, raw: bool, bytes: bool) {
        if self.language == Language::Python {
            self.string_python(start, quote, raw, bytes, false);
        } else {
            self.string_script(start, quote);
        }
    }

    fn string_script(&mut self, start: usize, quote: u8) {
        self.position += 1;
        let mut text = String::new();
        let mut run = self.position;
        while let Some(byte) = self.peek(0) {
            if byte == quote {
                text.push_str(&self.text[run..self.position]);
                self.position += 1;
                self.tokens.push(Token { kind: Kind::Str, start, end: self.position, pieces: Some(vec![Piece::Text(text)]) });
                return;
            }
            if byte == b'\n' {
                break;
            }
            if byte == b'\\' {
                text.push_str(&self.text[run..self.position]);
                self.position += 1;
                self.escape(&mut text);
                run = self.position;
                continue;
            }
            self.position += 1;
        }
        // Unterminated: stop at the line end; the token is not SQL.
        self.tokens.push(Token { kind: Kind::Other, start, end: self.position, pieces: None });
    }

    /// Decode one escape after its backslash; leaves `position` after it.
    fn escape(&mut self, out: &mut String) {
        let Some(byte) = self.peek(0) else { return };
        self.position += 1;
        match byte {
            b'n' => out.push('\n'),
            b't' => out.push('\t'),
            b'r' => out.push('\r'),
            b'0' => out.push('\0'),
            b'\n' => {}
            b'\r' => {
                if self.peek(0) == Some(b'\n') {
                    self.position += 1;
                }
            }
            b'x' => self.hex(2, out),
            b'u' => {
                if self.peek(0) == Some(b'{') {
                    let close = self.bytes[self.position..].iter().position(|b| *b == b'}').map(|n| self.position + n);
                    if let Some(close) = close {
                        let digits = &self.text[self.position + 1..close];
                        if let Some(c) = u32::from_str_radix(digits, 16).ok().and_then(char::from_u32) {
                            out.push(c);
                        }
                        self.position = close + 1;
                    }
                } else {
                    self.hex(4, out);
                }
            }
            _ => {
                // Any other escaped character stands for itself; step over a whole
                // UTF-8 sequence.
                let start = self.position - 1;
                let mut end = self.position;
                while end < self.bytes.len() && (self.bytes[end] & 0xC0) == 0x80 {
                    end += 1;
                }
                self.position = end;
                out.push_str(&self.text[start..end]);
            }
        }
    }

    fn hex(&mut self, digits: usize, out: &mut String) {
        let end = self.position + digits;
        if let Some(c) = self.text.get(self.position..end).and_then(|d| u32::from_str_radix(d, 16).ok()).and_then(char::from_u32) {
            out.push(c);
            self.position = end;
        }
    }

    fn string_python(&mut self, start: usize, quote: u8, raw: bool, bytes: bool, formatted: bool) {
        let triple = self.peek(1) == Some(quote) && self.peek(2) == Some(quote);
        self.position += if triple { 3 } else { 1 };
        let mut pieces: Vec<Piece> = Vec::new();
        let mut text = String::new();
        let mut run = self.position;
        loop {
            let Some(byte) = self.peek(0) else {
                self.tokens.push(Token { kind: Kind::Other, start, end: self.position, pieces: None });
                return;
            };
            if byte == quote && (!triple || (self.peek(1) == Some(quote) && self.peek(2) == Some(quote))) {
                text.push_str(&self.text[run..self.position]);
                self.position += if triple { 3 } else { 1 };
                break;
            }
            if byte == b'\n' && !triple {
                self.tokens.push(Token { kind: Kind::Other, start, end: self.position, pieces: None });
                return;
            }
            if byte == b'\\' && !raw {
                text.push_str(&self.text[run..self.position]);
                self.position += 1;
                self.escape(&mut text);
                run = self.position;
                continue;
            }
            if byte == b'\\' && raw {
                self.position += 2;
                continue;
            }
            if formatted && byte == b'{' {
                if self.peek(1) == Some(b'{') {
                    text.push_str(&self.text[run..self.position]);
                    text.push('{');
                    self.position += 2;
                    run = self.position;
                    continue;
                }
                text.push_str(&self.text[run..self.position]);
                if !text.is_empty() {
                    pieces.push(Piece::Text(std::mem::take(&mut text)));
                }
                pieces.push(Piece::Dynamic);
                // Skip the replacement field, balancing braces and nested strings.
                self.position += 1;
                let mut depth = 1usize;
                while let Some(inner) = self.peek(0) {
                    match inner {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                self.position += 1;
                                break;
                            }
                        }
                        b'\n' if !triple => break,
                        _ => {}
                    }
                    self.position += 1;
                }
                run = self.position;
                continue;
            }
            if formatted && byte == b'}' && self.peek(1) == Some(b'}') {
                text.push_str(&self.text[run..self.position]);
                text.push('}');
                self.position += 2;
                run = self.position;
                continue;
            }
            self.position += 1;
        }
        if !text.is_empty() || pieces.is_empty() {
            pieces.push(Piece::Text(text));
        }
        let token = Token { kind: Kind::Str, start, end: self.position, pieces: if bytes { None } else { Some(pieces) } };
        self.tokens.push(token);
    }

    fn template(&mut self) {
        let start = self.position;
        self.position += 1;
        let mut pieces: Vec<Piece> = Vec::new();
        let mut text = String::new();
        let mut run = self.position;
        loop {
            let Some(byte) = self.peek(0) else {
                self.tokens.push(Token { kind: Kind::Other, start, end: self.position, pieces: None });
                return;
            };
            match byte {
                b'`' => {
                    text.push_str(&self.text[run..self.position]);
                    self.position += 1;
                    break;
                }
                b'\\' => {
                    text.push_str(&self.text[run..self.position]);
                    self.position += 1;
                    self.escape(&mut text);
                    run = self.position;
                }
                b'$' if self.peek(1) == Some(b'{') => {
                    text.push_str(&self.text[run..self.position]);
                    if !text.is_empty() {
                        pieces.push(Piece::Text(std::mem::take(&mut text)));
                    }
                    pieces.push(Piece::Dynamic);
                    self.position += 2;
                    self.skip_template_expression();
                    run = self.position;
                }
                _ => self.position += 1,
            }
        }
        if !text.is_empty() || pieces.is_empty() {
            pieces.push(Piece::Text(text));
        }
        self.tokens.push(Token { kind: Kind::Str, start, end: self.position, pieces: Some(pieces) });
    }

    /// Step over the expression of a `${ ... }`, whose opening is consumed.
    fn skip_template_expression(&mut self) {
        let mut depth = 1usize;
        while let Some(byte) = self.peek(0) {
            match byte {
                b'{' => {
                    depth += 1;
                    self.position += 1;
                }
                b'}' => {
                    depth -= 1;
                    self.position += 1;
                    if depth == 0 {
                        return;
                    }
                }
                b'\'' | b'"' => {
                    let at = self.position;
                    let before = self.tokens.len();
                    self.string_script(at, byte);
                    self.tokens.truncate(before);
                }
                b'`' => {
                    let before = self.tokens.len();
                    self.template();
                    self.tokens.truncate(before);
                }
                b'/' if self.peek(1) == Some(b'/') => self.skip_line(),
                b'/' if self.peek(1) == Some(b'*') => self.skip_block(),
                _ => self.position += 1,
            }
        }
    }
}

/// Method names that take SQL text as their first argument in the common
/// drivers: better-sqlite3 (`prepare`, `exec`), node-postgres (`query`),
/// sqlite3 (`run`, `get`, `all`, `each`, `exec`, `prepare`), the Python DB-API
/// (`execute`, `executemany`, `executescript`), asyncpg/psycopg helpers.
const CALLS: &[&str] = &[
    "query", "queryRow", "queryArray", "queryObject", "execute", "executemany", "executescript", "exec", "run", "get", "all",
    "each", "iterate", "prepare", "raw", "fetch", "fetchrow", "fetchval", "fetchall", "fetchone", "fetchmany", "one", "many",
    "none", "any", "oneOrNone", "manyOrNone", "unsafe",
];

const LEADING_KEYWORDS: &[&str] = &[
    "select", "insert", "update", "delete", "with", "create", "alter", "drop", "pragma", "replace", "begin", "commit",
    "rollback", "savepoint", "release", "vacuum", "explain", "analyze", "attach", "detach", "reindex", "values", "truncate",
];

fn leading_word(text: &str) -> Option<String> {
    let mut rest = text.trim_start();
    loop {
        if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail).trim_start();
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail).trim_start();
        } else if let Some(after) = rest.strip_prefix('(') {
            rest = after.trim_start();
        } else {
            break;
        }
    }
    let word: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    (!word.is_empty()).then(|| word.to_ascii_lowercase())
}

fn contains_sql_clause(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [" from ", " into ", " where ", " join ", " set ", "\nfrom ", "\nwhere ", "\njoin "].iter().any(|needle| lower.contains(needle))
        || lower.starts_with("from ")
}

/// Operands of one argument expression.
struct Operands {
    pieces: Vec<Piece>,
    /// A non-literal operand, `%`, `.format` or an f-string interpolation.
    dynamic: bool,
    /// Index of the token after the expression.
    next: usize,
    /// Token range of the whole expression.
    first: usize,
    last: usize,
}

struct Scanner<'a> {
    text: &'a str,
    language: Language,
    tokens: Vec<Token>,
}

impl<'a> Scanner<'a> {
    fn punct(&self, index: usize) -> Option<u8> {
        let token = self.tokens.get(index)?;
        (token.kind == Kind::Punct).then(|| self.text.as_bytes()[token.start])
    }

    fn word(&self, index: usize) -> Option<&str> {
        let token = self.tokens.get(index)?;
        (token.kind == Kind::Ident).then(|| &self.text[token.start..token.end])
    }

    /// Skip a balanced bracket group starting at `index` (an opener); returns
    /// the index after its closer.
    fn skip_group(&self, mut index: usize) -> usize {
        let mut depth = 0usize;
        while index < self.tokens.len() {
            match self.punct(index) {
                Some(b'(' | b'[' | b'{') => depth += 1,
                Some(b')' | b']' | b'}') => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return index + 1;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        index
    }

    /// Consume a non-literal operand up to a top-level `+`, `,`, `)`, `;`, `}` or `]`.
    fn skip_operand(&self, mut index: usize) -> usize {
        while index < self.tokens.len() {
            match self.punct(index) {
                Some(b'(' | b'[' | b'{') => index = self.skip_group(index),
                Some(b'+' | b',' | b')' | b';' | b'}' | b']') => return index,
                _ => index += 1,
            }
        }
        index
    }

    fn operands(&self, mut index: usize) -> Option<Operands> {
        let first = index;
        let mut pieces = Vec::new();
        let mut dynamic = false;
        let mut last;
        let mut seen = false;
        loop {
            let token = self.tokens.get(index)?;
            if token.kind == Kind::Str {
                match &token.pieces {
                    Some(literal) => {
                        for piece in literal {
                            if matches!(piece, Piece::Dynamic) {
                                dynamic = true;
                            }
                            pieces.push(piece.clone());
                        }
                    }
                    // A byte string is not SQL text.
                    None => return None,
                }
                last = index;
                index += 1;
                seen = true;
                // Python adjacent literals concatenate implicitly.
                if self.language == Language::Python {
                    while self.tokens.get(index).is_some_and(|t| t.kind == Kind::Str) {
                        let Some(literal) = &self.tokens[index].pieces else { return None };
                        for piece in literal {
                            if matches!(piece, Piece::Dynamic) {
                                dynamic = true;
                            }
                            pieces.push(piece.clone());
                        }
                        last = index;
                        index += 1;
                    }
                    // `"..." % args` and `"...".format(...)` build the text.
                    if self.punct(index) == Some(b'%') {
                        dynamic = true;
                        index = self.skip_operand(index);
                        last = index.saturating_sub(1);
                    } else if self.punct(index) == Some(b'.') && self.word(index + 1) == Some("format") {
                        dynamic = true;
                        index = self.skip_operand(index);
                        last = index.saturating_sub(1);
                    }
                } else if self.punct(index) == Some(b'.') && matches!(self.word(index + 1), Some("trim" | "concat" | "replace" | "join" | "toString")) {
                    // `"...".concat(x)`, `.replace(...)`: the text is modified.
                    dynamic = true;
                    index = self.skip_operand(index);
                    last = index.saturating_sub(1);
                }
            } else {
                // A variable, a call, a member: run-time text.
                if !seen && self.punct(index).is_some_and(|c| matches!(c, b')' | b',')) {
                    return None;
                }
                let end = self.skip_operand(index);
                if end == index {
                    return None;
                }
                pieces.push(Piece::Dynamic);
                dynamic = true;
                last = end - 1;
                index = end;
                seen = true;
            }
            match self.punct(index) {
                Some(b'+') => index += 1,
                _ => break,
            }
        }
        seen.then_some(Operands { pieces, dynamic, next: index, first, last })
    }

    /// The SQL operand of `{ text: ..., values: [...] }` (node-postgres config).
    fn config_text(&self, open: usize) -> Option<usize> {
        let mut index = open + 1;
        let mut depth = 1usize;
        while index < self.tokens.len() && depth > 0 {
            match self.punct(index) {
                Some(b'{' | b'(' | b'[') => depth += 1,
                Some(b'}' | b')' | b']') => depth -= 1,
                _ => {
                    let key_here = depth == 1
                        && (self.word(index) == Some("text")
                            || self.tokens.get(index).is_some_and(|t| {
                                t.kind == Kind::Str && t.pieces.as_deref().is_some_and(|p| matches!(p, [Piece::Text(s)] if s == "text"))
                            }));
                    if key_here && self.punct(index + 1) == Some(b':') {
                        return Some(index + 2);
                    }
                }
            }
            index += 1;
        }
        None
    }
}

fn line_of(source: &[u8], offset: usize) -> u32 {
    u32::try_from(source[..offset.min(source.len())].iter().filter(|b| **b == b'\n').count() + 1).unwrap_or(u32::MAX)
}

fn dialect_hint(text: &str) -> Option<SqlDialect> {
    let lower = text.to_ascii_lowercase();
    let sqlite = ["better-sqlite3", "sqlite3", "node:sqlite", "bun:sqlite", "libsql", "aiosqlite", "expo-sqlite"].iter().any(|n| lower.contains(n));
    let postgres = ["'pg'", "\"pg\"", "pg-promise", "postgres", "psycopg", "asyncpg", "@neondatabase", "@vercel/postgres"].iter().any(|n| lower.contains(n));
    match (sqlite, postgres) {
        (true, false) => Some(SqlDialect::Sqlite),
        (false, true) => Some(SqlDialect::Postgres),
        _ => None,
    }
}

/// Positional placeholders of a Python driver, rewritten to forms the SQL
/// parsers accept: `%s` becomes `?` and `%(name)s` becomes `:name`.
fn normalize_placeholders(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        let tail = &rest[at + 1..];
        if let Some(after) = tail.strip_prefix('s') {
            out.push('?');
            rest = after;
        } else if let Some(named) = tail.strip_prefix('(') {
            match named.split_once(")s") {
                Some((name, after)) if !name.is_empty() && name.bytes().all(is_ident_part) => {
                    out.push(':');
                    out.push_str(name);
                    rest = after;
                }
                _ => {
                    out.push('%');
                    rest = tail;
                }
            }
        } else if let Some(after) = tail.strip_prefix('%') {
            out.push('%');
            rest = after;
        } else {
            out.push('%');
            rest = tail;
        }
    }
    out.push_str(rest);
    out
}

/// Find every query call of one application file.
///
/// `None` means the file is not a candidate, is not UTF-8, is over the scan
/// bound, or holds no SQL calls.
pub(super) fn scan(path: &str, source: &[u8]) -> Option<Scan> {
    let language = Language::of(path)?;
    if source.len() > MAX_SCAN_BYTES {
        return None;
    }
    let text = std::str::from_utf8(source).ok()?;
    // A cheap exit for the overwhelmingly common file with no SQL in it.
    let lower = text.to_ascii_lowercase();
    if !LEADING_KEYWORDS.iter().any(|keyword| lower.contains(keyword)) {
        return None;
    }
    let tokens = Lexer::new(text, language).run();
    let scanner = Scanner { text, language, tokens };
    let mut composed = Vec::new();
    let mut fragments = Vec::new();
    let mut dropped = 0usize;
    let mut index = 0usize;
    while index < scanner.tokens.len() {
        let is_call = scanner.punct(index) == Some(b'.')
            || (scanner.punct(index) == Some(b'?') && scanner.punct(index + 1) == Some(b'.'));
        let name_at = if scanner.punct(index) == Some(b'?') { index + 2 } else { index + 1 };
        let Some(name) = is_call.then(|| scanner.word(name_at)).flatten() else {
            index += 1;
            continue;
        };
        if !CALLS.contains(&name) {
            index += 1;
            continue;
        }
        let mut open = name_at + 1;
        // TypeScript generics: `query<Row>(`.
        if language == Language::Script && scanner.punct(open) == Some(b'<') {
            let mut depth = 0usize;
            let mut probe = open;
            while probe < scanner.tokens.len() && probe < open + 64 {
                match scanner.punct(probe) {
                    Some(b'<') => depth += 1,
                    Some(b'>') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Some(b';' | b')') => break,
                    _ => {}
                }
                probe += 1;
            }
            if depth == 0 && scanner.punct(probe) == Some(b'>') {
                open = probe + 1;
            }
        }
        if scanner.punct(open) != Some(b'(') {
            index += 1;
            continue;
        }
        let mut argument = open + 1;
        if scanner.punct(argument) == Some(b'{') {
            let Some(inner) = scanner.config_text(argument) else {
                index += 1;
                continue;
            };
            argument = inner;
        }
        let Some(operands) = scanner.operands(argument) else {
            index += 1;
            continue;
        };
        // Admission: the literal text must look like SQL.
        let first_text = operands.pieces.iter().find_map(|piece| match piece {
            Piece::Text(text) if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        });
        // A lone keyword is SQL only when it is a complete statement by itself.
        let bare = |text: &str, word: &str| operands.pieces.len() == 1 && text.trim().trim_end_matches(';').trim().eq_ignore_ascii_case(word);
        let leads_sql = matches!(operands.pieces.first(), Some(Piece::Text(_)))
            && first_text.is_some_and(|text| {
                leading_word(text).is_some_and(|word| {
                    LEADING_KEYWORDS.contains(&word.as_str())
                        && (!bare(text, &word) || matches!(word.as_str(), "begin" | "commit" | "rollback" | "vacuum"))
                })
            });
        let joined_static = operands.pieces.iter().filter_map(|p| if let Piece::Text(t) = p { Some(t.as_str()) } else { None }).collect::<Vec<_>>().join(" ");
        let looks_sql = leads_sql || (matches!(operands.pieces.first(), Some(Piece::Dynamic)) && contains_sql_clause(&joined_static));
        if !looks_sql {
            index = operands.next.max(index + 1);
            continue;
        }
        if fragments.len() >= MAX_FRAGMENTS {
            dropped += 1;
            index = operands.next.max(index + 1);
            continue;
        }
        let mut statement = String::new();
        for piece in &operands.pieces {
            match piece {
                Piece::Text(text) => statement.push_str(&if language == Language::Python { normalize_placeholders(text) } else { text.clone() }),
                Piece::Dynamic => statement.push_str(DYNAMIC_PLACEHOLDER),
            }
        }
        // The worker reads one statement per call; a call's own terminators and
        // trailing comment-only tail must not merge it with the next call.
        let trimmed = statement.trim_end().trim_end_matches(';').trim_end();
        let text_start = composed.len();
        composed.extend_from_slice(trimmed.as_bytes());
        let text_end = composed.len();
        // A final line holding `--` may end in a line comment, which would swallow
        // the terminator and merge the next call into this one: end it on its own line.
        let last_line = trimmed.rsplit('\n').next().unwrap_or(trimmed);
        composed.extend_from_slice(if last_line.contains("--") { b"\n;\n" } else { b";\n" });
        let start = scanner.tokens[operands.first].start;
        let end = scanner.tokens[operands.last].end;
        fragments.push(Fragment {
            start,
            end,
            line: line_of(source, start),
            dynamic: operands.dynamic,
            text_start,
            text_end,
        });
        index = operands.next.max(index + 1);
    }
    if fragments.is_empty() {
        return None;
    }
    Some(Scan { composed, fragments, dialect: dialect_hint(text), dropped })
}

/// The bytes of an embedded host, re-derived from its application file's bytes.
pub(super) fn composed_for(virtual_path: &str, source: &[u8]) -> Option<Vec<u8>> {
    scan(real_path(virtual_path)?, source).map(|scan| scan.composed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn composed(path: &str, source: &str) -> (String, Vec<bool>) {
        let scan = scan(path, source.as_bytes()).expect("a scan");
        (String::from_utf8(scan.composed).unwrap(), scan.fragments.iter().map(|f| f.dynamic).collect())
    }

    #[test]
    fn better_sqlite3_and_node_postgres_literals_are_static() {
        let source = r#"
import Database from 'better-sqlite3';
const db = new Database('app.db');
const find = db.prepare('SELECT email, balance FROM accounts WHERE id = ?');
db.exec(`CREATE TABLE IF NOT EXISTS audit (id INTEGER)`);
const rows = await client.query<Row>("UPDATE accounts SET balance = balance - $1 WHERE id = $2", [1, 2]);
await pool.query({ text: 'DELETE FROM orders WHERE id = $1', values: [1] });
"#;
        let (text, dynamic) = composed("app/db.ts", source);
        assert_eq!(
            text,
            "SELECT email, balance FROM accounts WHERE id = ?;\nCREATE TABLE IF NOT EXISTS audit (id INTEGER);\n\
             UPDATE accounts SET balance = balance - $1 WHERE id = $2;\nDELETE FROM orders WHERE id = $1;\n"
        );
        assert_eq!(dynamic, vec![false; 4]);
        let scan = scan("app/db.ts", source.as_bytes()).unwrap();
        assert_eq!(scan.dialect, Some(SqlDialect::Sqlite));
        assert_eq!(&source[scan.fragments[0].start..scan.fragments[0].end], "'SELECT email, balance FROM accounts WHERE id = ?'");
        assert_eq!(scan.fragments[0].line, 4);
    }

    #[test]
    fn a_trailing_line_comment_does_not_swallow_the_terminator() {
        let source = "db.exec(`SELECT 1 FROM a -- the first`);\ndb.exec('SELECT 2 FROM b');\n";
        let scan = scan("a.js", source.as_bytes()).unwrap();
        let text = String::from_utf8(scan.composed.clone()).unwrap();
        assert_eq!(text, "SELECT 1 FROM a -- the first\n;\nSELECT 2 FROM b;\n");
        let [first, second] = &scan.fragments[..] else { panic!("two fragments") };
        assert_eq!(&text[first.text_start..first.text_end], "SELECT 1 FROM a -- the first");
        assert_eq!(&text[second.text_start..second.text_end], "SELECT 2 FROM b");
    }

    #[test]
    fn built_queries_are_dynamic_and_keep_their_static_text() {
        let source = "const a = db.all(`SELECT ${cols} FROM accounts WHERE id = ${id}`);\n\
                      const b = db.get('SELECT * FROM ' + table + ' WHERE id = 1');\n\
                      const c = db.all('SELECT a ' + 'FROM accounts');\n";
        let (text, dynamic) = composed("a.js", source);
        assert_eq!(
            text,
            "SELECT nudox_dynamic FROM accounts WHERE id = nudox_dynamic;\nSELECT * FROM nudox_dynamic WHERE id = 1;\nSELECT a FROM accounts;\n"
        );
        assert_eq!(dynamic, vec![true, true, false]);
    }

    #[test]
    fn non_sql_calls_and_lookalikes_are_ignored() {
        let source = "map.get('key'); Promise.all([a]); document.querySelector('#a'); db.run(sqlFromElsewhere); x.query('hello world');\n\
                      // db.query('SELECT 1 FROM ghost')\n/* db.query('SELECT 1 FROM ghost') */\nconst s = \"db.query('SELECT 1 FROM ghost')\";\n\
                      const r = /db.query('select/.test(x); const t = a / b; db.query('select 1 from real');\n";
        let (text, _) = composed("a.ts", source);
        assert_eq!(text, "select 1 from real;\n");
        assert!(scan("a.ts", b"const x = map.get('select');").is_none());
    }

    #[test]
    fn python_literals_formats_and_placeholders() {
        let source = "import sqlite3\n\
            cur.execute(\"SELECT email FROM accounts WHERE id = %s\", (1,))\n\
            cur.execute('''\n  UPDATE accounts\n  SET balance = 1\n''')\n\
            cur.execute(\"SELECT * FROM %s\" % table)\n\
            cur.execute(f\"SELECT {cols} FROM accounts\")\n\
            cur.execute(\"SELECT a \"\n            \"FROM accounts WHERE n = %(n)s\", {'n': 1})\n\
            cur.executemany(b'not sql', x)\n\
            # cur.execute('SELECT 1 FROM ghost')\n";
        let scan = scan("app/db.py", source.as_bytes()).unwrap();
        let text = String::from_utf8(scan.composed.clone()).unwrap();
        assert_eq!(
            text,
            "SELECT email FROM accounts WHERE id = ?;\n\n  UPDATE accounts\n  SET balance = 1;\nSELECT * FROM ?;\n\
             SELECT nudox_dynamic FROM accounts;\nSELECT a FROM accounts WHERE n = :n;\n"
        );
        assert_eq!(scan.fragments.iter().map(|f| f.dynamic).collect::<Vec<_>>(), vec![false, false, true, true, false]);
        assert_eq!(scan.dialect, Some(SqlDialect::Sqlite));
    }

    #[test]
    fn composition_is_deterministic_and_maps_statements() {
        let source = b"db.prepare('SELECT 1 FROM t'); db.prepare('SELECT 2 FROM u');";
        let first = scan("x.ts", source).unwrap();
        assert_eq!(composed_for("x.ts#embedded.sql", source), Some(first.composed.clone()));
        let at = first.composed.windows(6).position(|w| w == b"SELECT").unwrap();
        assert_eq!(first.fragment_at(at + 5).map(|f| f.start), Some(first.fragments[0].start));
        let second = first.composed.windows(8).position(|w| w == b"SELECT 2").unwrap();
        assert_eq!(first.fragment_at(second + 3).map(|f| f.start), Some(first.fragments[1].start));
        assert!(composed_for("x.sql#embedded.sql", source).is_none());
        assert!(real_path("x.ts#embedded.sql").is_some() && real_path("x.ts").is_none());
    }

    #[test]
    fn large_and_non_utf8_files_are_not_scanned() {
        assert!(scan("a.ts", &vec![b' '; MAX_SCAN_BYTES + 1]).is_none());
        assert!(scan("a.ts", &[0xff, 0xfe]).is_none());
        assert!(scan("a.rb", b"db.query('select 1 from t')").is_none());
    }
}
