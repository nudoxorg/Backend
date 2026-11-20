//! Lexical colouring for the two languages whose meaning the app takes from
//! the owner's native analyses (SQL from syntaqlite and pgrust, CSS from
//! Stylo), never from a grammar bundled in the app.
//!
//! This is paint, not parsing. It reads characters left to right, recognises
//! comments, strings, numbers, names the language reserves and the shapes CSS
//! gives its punctuation, and says nothing about what a name refers to. It
//! never fails: text it does not follow stays plain.

use super::Role;
use std::ops::Range;

/// The role runs for SQL text (any dialect: it colours only what every dialect
/// shares).
#[must_use]
pub(super) fn sql(source: &str) -> Vec<(Range<usize>, Role)> {
    let bytes = source.as_bytes();
    let mut runs = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        match byte {
            b'-' if bytes.get(at + 1) == Some(&b'-') => {
                let end = line_end(bytes, at);
                runs.push((at..end, Role::Comment));
                at = end;
            }
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                let end = block_comment_end(bytes, at + 2);
                runs.push((at..end, Role::Comment));
                at = end;
            }
            b'\'' => {
                let end = quoted_end(bytes, at, b'\'', false);
                runs.push((at..end, Role::String));
                at = end;
            }
            b'"' | b'`' => {
                let end = quoted_end(bytes, at, byte, false);
                runs.push((at..end, Role::Type));
                at = end;
            }
            b'$' => {
                if let Some(end) = dollar_quoted_end(bytes, at) {
                    runs.push((at..end, Role::String));
                    at = end;
                } else {
                    let end = parameter_end(bytes, at);
                    if end > at + 1 {
                        runs.push((at..end, Role::Parameter));
                    }
                    at = end.max(at + 1);
                }
            }
            b'?' => {
                runs.push((at..at + 1, Role::Parameter));
                at += 1;
            }
            b':' | b'@' if bytes.get(at + 1).is_some_and(|next| is_word(*next)) => {
                let end = word_end(bytes, at + 1);
                runs.push((at..end, Role::Parameter));
                at = end;
            }
            b'0'..=b'9' => {
                let end = number_end(bytes, at);
                runs.push((at..end, Role::Number));
                at = end;
            }
            b'.' if bytes.get(at + 1).is_some_and(u8::is_ascii_digit)
                && !bytes.get(at.wrapping_sub(1)).is_some_and(|before| is_word(*before)) =>
            {
                let end = number_end(bytes, at + 1);
                runs.push((at..end, Role::Number));
                at = end;
            }
            _ if is_word_start(byte) => {
                let end = word_end(bytes, at);
                let word = &source[at..end];
                if let Some(role) = sql_word(word, next_non_space(bytes, end)) {
                    runs.push((at..end, role));
                }
                at = end;
            }
            _ => at += utf8_len(byte),
        }
    }
    runs
}

/// The role runs for CSS text.
#[must_use]
pub(super) fn css(source: &str) -> Vec<(Range<usize>, Role)> {
    let bytes = source.as_bytes();
    let mut runs = Vec::new();
    // Whether the statement being read ends in `{` (a selector or at-rule
    // prelude) or in `;`/`}` (a declaration), found by looking ahead from its
    // first character. `None` between statements.
    let mut statement: Option<Statement> = None;
    // Whether a declaration's value (after its `:`) is being read.
    let mut in_value = false;
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if statement.is_none() && !byte.is_ascii_whitespace() && !matches!(byte, b'{' | b'}' | b';') {
            if byte == b'/' && bytes.get(at + 1) == Some(&b'*') {
                // A comment between statements does not start one.
            } else {
                statement = Some(statement_kind(bytes, at));
                in_value = false;
            }
        }
        match byte {
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                let end = block_comment_end(bytes, at + 2);
                runs.push((at..end, Role::Comment));
                at = end;
            }
            b'"' | b'\'' => {
                let end = quoted_end(bytes, at, byte, true);
                runs.push((at..end, Role::String));
                at = end;
            }
            b'{' | b'}' | b';' => {
                statement = None;
                in_value = false;
                at += 1;
            }
            b'@' if bytes.get(at + 1).is_some_and(|next| is_ident(*next)) => {
                let end = ident_end(bytes, at + 1);
                runs.push((at..end, Role::Macro));
                at = end;
            }
            b'#' if bytes.get(at + 1).is_some_and(|next| is_ident(*next)) => {
                let end = ident_end(bytes, at + 1);
                let role = if in_value { Role::Constant } else { Role::Type };
                runs.push((at..end, role));
                at = end;
            }
            b'.' if statement == Some(Statement::Rule)
                && bytes.get(at + 1).is_some_and(|next| is_ident(*next)) =>
            {
                let end = ident_end(bytes, at + 1);
                runs.push((at..end, Role::Type));
                at = end;
            }
            b'!' => {
                let word = ident_end(bytes, at + 1);
                if source[at + 1..word].eq_ignore_ascii_case("important") {
                    runs.push((at..word, Role::Keyword));
                    at = word;
                } else {
                    at += 1;
                }
            }
            b':' => {
                if statement == Some(Statement::Declaration) {
                    in_value = true;
                }
                at += 1;
            }
            b'0'..=b'9' | b'+' | b'-' | b'.'
                if starts_number(bytes, at) && !(in_value_ident_continuation(bytes, at)) =>
            {
                let end = number_end(bytes, at);
                let unit = ident_end(bytes, end);
                runs.push((at..unit, Role::Number));
                at = unit;
            }
            _ if is_ident(byte) => {
                let end = ident_end(bytes, at);
                let word = &source[at..end];
                let call = bytes.get(end) == Some(&b'(');
                let role = match statement {
                    _ if call => Some(Role::Function),
                    Some(Statement::Declaration) if !in_value => {
                        Some(if word.starts_with("--") { Role::Constant } else { Role::Attribute })
                    }
                    Some(Statement::Declaration) if word.starts_with("--") => Some(Role::Constant),
                    Some(Statement::Declaration) => css_value_word(word),
                    Some(Statement::Rule) => css_selector_word(bytes, at, word),
                    Some(Statement::AtRule) | None => None,
                };
                if let Some(role) = role {
                    runs.push((at..end, role));
                }
                at = end;
            }
            _ => at += utf8_len(byte),
        }
    }
    runs
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Statement {
    /// A selector list, up to its `{`.
    Rule,
    /// An at-rule's prelude: `@media (min-width: 40em)`.
    AtRule,
    /// A property and its value, up to `;` or `}`.
    Declaration,
}

/// What the statement starting at `from` is, read by looking for the first of
/// `{`, `;` or `}` outside strings, comments and brackets.
fn statement_kind(bytes: &[u8], from: usize) -> Statement {
    if bytes[from] == b'@' {
        return Statement::AtRule;
    }
    let mut depth = 0usize;
    let mut at = from;
    while at < bytes.len() {
        match bytes[at] {
            b'/' if bytes.get(at + 1) == Some(&b'*') => {
                at = block_comment_end(bytes, at + 2);
                continue;
            }
            b'"' | b'\'' => {
                at = quoted_end(bytes, at, bytes[at], true);
                continue;
            }
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth = depth.saturating_sub(1),
            b'{' if depth == 0 => return Statement::Rule,
            b';' | b'}' if depth == 0 => return Statement::Declaration,
            _ => {}
        }
        at += 1;
    }
    // Text that ends first is a declaration being typed.
    Statement::Declaration
}

fn css_selector_word(bytes: &[u8], at: usize, word: &str) -> Option<Role> {
    // After a `:` or `::` the word is a pseudo-class or pseudo-element.
    let colons = bytes[..at].iter().rev().take_while(|byte| **byte == b':').count();
    if colons > 0 {
        return Some(Role::Contract);
    }
    // After `[`, an attribute name; elsewhere a type selector (`a`, `div`).
    let before = bytes[..at].iter().rev().find(|byte| !byte.is_ascii_whitespace());
    if before == Some(&b'[') {
        return Some(Role::Attribute);
    }
    if word.starts_with("--") {
        return Some(Role::Constant);
    }
    let after = next_non_space(bytes, at + word.len());
    if matches!(after, Some(b'=' | b']' | b'~' | b'|' | b'^' | b'$' | b'*')) && before != Some(&b'.') {
        return Some(Role::Attribute);
    }
    // A class was already painted by its `.`; its name is part of that run.
    (before != Some(&b'.')).then_some(Role::Keyword)
}

fn css_value_word(word: &str) -> Option<Role> {
    const NAMED: &[&str] = &[
        "auto", "none", "inherit", "initial", "unset", "revert", "normal", "transparent", "currentcolor",
        "solid", "dashed", "dotted", "block", "inline", "flex", "grid", "absolute", "relative", "fixed",
        "sticky", "hidden", "visible", "bold", "italic", "center", "left", "right", "top", "bottom",
    ];
    NAMED
        .iter()
        .any(|name| word.eq_ignore_ascii_case(name))
        .then_some(Role::Constant)
}

fn sql_word(word: &str, next: Option<u8>) -> Option<Role> {
    const KEYWORDS: &[&str] = &[
        "ADD", "ALL", "ALTER", "AND", "AS", "ASC", "BEGIN", "BETWEEN", "BY", "CASCADE", "CASE", "CAST", "CHECK",
        "COLLATE", "COLUMN", "COMMIT", "CONSTRAINT", "CREATE", "CROSS", "DEFAULT", "DELETE", "DESC", "DISTINCT",
        "DROP", "ELSE", "END", "EXCEPT", "EXISTS", "EXPLAIN", "FOREIGN", "FROM", "FULL", "GROUP", "HAVING", "IF",
        "IN", "INDEX", "INNER", "INSERT", "INTERSECT", "INTO", "IS", "JOIN", "KEY", "LEFT", "LIKE", "LIMIT",
        "NATURAL", "NOT", "OFFSET", "ON", "OR", "ORDER", "OUTER", "OVER", "PARTITION", "PRIMARY", "RECURSIVE",
        "REFERENCES", "RETURNING", "RIGHT", "ROLLBACK", "SELECT", "SET", "TABLE", "THEN", "TRIGGER", "UNION",
        "UNIQUE", "UPDATE", "USING", "VALUES", "VIEW", "WHEN", "WHERE", "WINDOW", "WITH",
    ];
    const TYPES: &[&str] = &[
        "BIGINT", "BLOB", "BOOLEAN", "CHAR", "DATE", "DECIMAL", "DOUBLE", "FLOAT", "INT", "INTEGER", "JSON",
        "JSONB", "NUMERIC", "REAL", "SERIAL", "SMALLINT", "TEXT", "TIME", "TIMESTAMP", "UUID", "VARCHAR",
    ];
    const CONSTANTS: &[&str] = &["NULL", "TRUE", "FALSE", "CURRENT_DATE", "CURRENT_TIME", "CURRENT_TIMESTAMP"];
    let is = |list: &[&str]| list.iter().any(|candidate| word.eq_ignore_ascii_case(candidate));
    let call = next == Some(b'(');
    if is(KEYWORDS) {
        // `LEFT(x, 2)` and `IF(a, b, c)` are functions that share a reserved spelling.
        let function = call && ["LEFT", "RIGHT", "IF"].iter().any(|name| word.eq_ignore_ascii_case(name));
        Some(if function { Role::Function } else { Role::Keyword })
    } else if is(CONSTANTS) {
        Some(Role::Constant)
    } else if is(TYPES) && !(call && ["DATE", "TIME", "TIMESTAMP"].iter().any(|name| word.eq_ignore_ascii_case(name))) {
        Some(Role::Type)
    } else if call {
        Some(Role::Function)
    } else {
        None
    }
}

fn line_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |end| from + end)
}

/// The end of a `/* ... */` comment whose body starts at `from`; an unclosed
/// comment runs to the end of the text.
fn block_comment_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .windows(2)
        .position(|pair| pair == b"*/")
        .map_or(bytes.len(), |end| from + end + 2)
}

/// The end of a quoted run opening at `open`. SQL doubles its quote to escape
/// it; CSS (`backslash`) escapes with `\`. An unclosed quote runs to the end of
/// the line in CSS, whose strings cannot span lines, and to the end of the text
/// in SQL.
fn quoted_end(bytes: &[u8], open: usize, quote: u8, backslash: bool) -> usize {
    let mut at = open + 1;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' if backslash => at += 2,
            byte if byte == quote => {
                if !backslash && bytes.get(at + 1) == Some(&quote) {
                    at += 2;
                } else {
                    return at + 1;
                }
            }
            b'\n' if backslash => return at,
            _ => at += 1,
        }
    }
    bytes.len()
}

/// `$tag$ ... $tag$` (PostgreSQL), when `from` opens one.
fn dollar_quoted_end(bytes: &[u8], from: usize) -> Option<usize> {
    let tag_end = bytes[from + 1..].iter().position(|byte| *byte == b'$')? + from + 1;
    let tag = &bytes[from..=tag_end];
    if tag[1..tag.len() - 1].iter().any(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'_')) {
        return None;
    }
    if tag.len() > 2 && tag[1].is_ascii_digit() {
        return None;
    }
    let body = tag_end + 1;
    Some(
        bytes[body..]
            .windows(tag.len())
            .position(|window| window == tag)
            .map_or(bytes.len(), |end| body + end + tag.len()),
    )
}

fn parameter_end(bytes: &[u8], from: usize) -> usize {
    let mut at = from + 1;
    while bytes.get(at).is_some_and(u8::is_ascii_digit) {
        at += 1;
    }
    at
}

fn number_end(bytes: &[u8], from: usize) -> usize {
    let mut at = from;
    if matches!(bytes.get(at), Some(b'+' | b'-')) {
        at += 1;
    }
    if bytes.get(at) == Some(&b'0') && matches!(bytes.get(at + 1), Some(b'x' | b'X')) {
        at += 2;
        while bytes.get(at).is_some_and(u8::is_ascii_hexdigit) {
            at += 1;
        }
        return at;
    }
    while bytes.get(at).is_some_and(|byte| byte.is_ascii_digit() || *byte == b'_') {
        at += 1;
    }
    if bytes.get(at) == Some(&b'.') && bytes.get(at + 1).is_some_and(u8::is_ascii_digit) {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
    }
    if matches!(bytes.get(at), Some(b'e' | b'E')) {
        let mut exponent = at + 1;
        if matches!(bytes.get(exponent), Some(b'+' | b'-')) {
            exponent += 1;
        }
        if bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
            while bytes.get(exponent).is_some_and(u8::is_ascii_digit) {
                exponent += 1;
            }
            at = exponent;
        }
    }
    at
}

/// Whether a CSS number begins at `at`: a digit, or a sign or dot before one.
fn starts_number(bytes: &[u8], at: usize) -> bool {
    match bytes[at] {
        b'0'..=b'9' => true,
        b'+' | b'-' => match bytes.get(at + 1) {
            Some(next) if next.is_ascii_digit() => true,
            Some(b'.') => bytes.get(at + 2).is_some_and(u8::is_ascii_digit),
            _ => false,
        },
        b'.' => bytes.get(at + 1).is_some_and(u8::is_ascii_digit),
        _ => false,
    }
}

/// A `-` or digit that is part of an identifier already being read (`h2`,
/// `col-4`) is not a number; the identifier branch consumes those, so this only
/// guards a sign that follows an identifier character.
fn in_value_ident_continuation(bytes: &[u8], at: usize) -> bool {
    matches!(bytes[at], b'+' | b'-')
        && at > 0
        && (is_ident(bytes[at - 1]) && bytes[at - 1] != b'(' && !bytes[at - 1].is_ascii_whitespace())
}

fn next_non_space(bytes: &[u8], from: usize) -> Option<u8> {
    bytes[from.min(bytes.len())..]
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace())
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

fn word_end(bytes: &[u8], from: usize) -> usize {
    let mut at = from;
    while bytes.get(at).is_some_and(|byte| is_word(*byte)) {
        at += 1;
    }
    at
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-') || byte >= 0x80 || byte == b'\\'
}

fn ident_end(bytes: &[u8], from: usize) -> usize {
    let mut at = from;
    while let Some(byte) = bytes.get(at) {
        if *byte == b'\\' {
            at += 2;
        } else if is_ident(*byte) {
            at += 1;
        } else {
            break;
        }
    }
    at.min(bytes.len())
}

/// Bytes in the UTF-8 sequence led by `byte`, so a scan steps over whole
/// characters and every run lands on a boundary.
fn utf8_len(byte: u8) -> usize {
    match byte {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::{css, sql};
    use crate::code::{Lang, Role, highlight_now};
    use std::ops::Range;

    fn roles(source: &str, runs: Vec<(Range<usize>, Role)>) -> Vec<(String, Role)> {
        runs.into_iter().map(|(range, role)| (source[range].to_owned(), role)).collect()
    }

    #[test]
    fn sql_marks_comments_strings_names_numbers_and_parameters() {
        let source = "-- c\nSELECT u.id, count(*) AS n, 'it''s' FROM \"users\" u WHERE u.age >= 18 AND x = :id /* b */ LIMIT 10;";
        let got = roles(source, sql(source));
        for expected in [
            ("-- c", Role::Comment),
            ("SELECT", Role::Keyword),
            ("count", Role::Function),
            ("'it''s'", Role::String),
            ("\"users\"", Role::Type),
            (":id", Role::Parameter),
            ("18", Role::Number),
            ("/* b */", Role::Comment),
        ] {
            assert!(got.contains(&(expected.0.to_owned(), expected.1)), "{expected:?} in {got:?}");
        }
        // Names the language does not reserve stay plain: the app claims
        // nothing about what `u` or `n` refer to.
        assert!(!got.iter().any(|(word, _)| word == "u" || word == "n"), "{got:?}");
    }

    #[test]
    fn sql_dollar_quotes_and_reserved_function_spellings() {
        let source = "SELECT $q$it's$q$, left(name, 2), $1 FROM t";
        let got = roles(source, sql(source));
        assert!(got.contains(&("$q$it's$q$".to_owned(), Role::String)), "{got:?}");
        assert!(got.contains(&("left".to_owned(), Role::Function)), "{got:?}");
        assert!(got.contains(&("$1".to_owned(), Role::Parameter)), "{got:?}");
    }

    #[test]
    fn css_separates_selectors_properties_values_and_custom_properties() {
        let source = "/* t */\n@media (min-width: 40em) {\n  a.btn:hover > span, #main {\n    color: #fff;\n    margin: -1.5rem auto !important;\n    --brand: rgb(1 2 3);\n    background: url(\"x.png\");\n  }\n}\n";
        let got = roles(source, css(source));
        for expected in [
            ("/* t */", Role::Comment),
            ("@media", Role::Macro),
            (".btn", Role::Type),
            ("hover", Role::Contract),
            ("#main", Role::Type),
            ("color", Role::Attribute),
            ("#fff", Role::Constant),
            ("-1.5rem", Role::Number),
            ("!important", Role::Keyword),
            ("--brand", Role::Constant),
            ("rgb", Role::Function),
            ("\"x.png\"", Role::String),
        ] {
            assert!(got.contains(&(expected.0.to_owned(), expected.1)), "{expected:?} in {got:?}");
        }
    }

    #[test]
    fn a_class_and_a_hex_colour_are_told_apart_by_where_they_stand() {
        let source = ".a { color: #abc }\n#a { color: red }";
        let got = roles(source, css(source));
        assert!(got.contains(&(".a".to_owned(), Role::Type)), "{got:?}");
        assert!(got.contains(&("#abc".to_owned(), Role::Constant)), "{got:?}");
        assert!(got.contains(&("#a".to_owned(), Role::Type)), "{got:?}");
    }

    #[test]
    fn neither_language_ever_fails_or_splits_a_character() {
        for source in [
            "", "'", "\"é", "/*", "$$ x", "$a$ é $a$", "a { b: \"é\\", "@", "#", "--", "-", ".", "é{é}é",
            "0x", "1e", "{{{{", "}}}}", "((((", "$1 $ $x", "a\\é { b: c\\", "😀 { 😀: 😀 }",
        ] {
            for runs in [sql(source), css(source)] {
                let mut last = 0;
                for (range, _) in runs {
                    assert!(
                        range.start >= last
                            && range.end <= source.len()
                            && source.is_char_boundary(range.start)
                            && source.is_char_boundary(range.end),
                        "{source:?} {range:?}"
                    );
                    last = range.end;
                }
            }
        }
    }

    #[test]
    fn the_highlighter_takes_sql_and_css_without_a_bundled_grammar() {
        assert_eq!(Lang::from_name("sql"), Some(Lang::Sql));
        assert_eq!(Lang::from_name(".CSS"), Some(Lang::Css));
        assert_eq!(Lang::Sql.grammar(), None);
        assert_eq!(Lang::Css.grammar(), None);
        let source = "SELECT 1; -- one";
        let highlighted = highlight_now(Lang::Sql, source.into());
        assert_eq!(highlighted.role_at(0), Some(Role::Keyword));
        assert_eq!(highlighted.role_at(source.find("--").expect("comment")), Some(Role::Comment));
    }
}
