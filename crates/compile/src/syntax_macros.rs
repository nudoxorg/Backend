//! Declarations written inside item-position macro invocations.
//!
//! A tags query runs against the parse tree, and a grammar parses a macro's
//! argument as an unstructured token tree: `pin_project! { pub struct
//! Compat<T> { .. } }` contains a struct no node says is one, so the struct
//! (and everything inside it) used to be missing from the declaration floor
//! even though a reader navigates to it daily.
//!
//! The arguments of an item-position invocation are source text like any
//! other. This module re-parses exactly the bytes between the delimiters as
//! Rust items (tree-sitter parses a byte range of the original buffer, so every
//! offset and line stays the original file's) and lets the ordinary tags query
//! run on the result. Three rules keep that honest:
//!
//! * The argument is accepted only when it parses without a syntax error. A
//!   macro with its own grammar is therefore never half-indexed from error
//!   recovery.
//! * Two ubiquitous macros are written in a grammar that is *almost* Rust
//!   (`lazy_static!` declares `static ref NAME`, `bitflags!` declares `struct
//!   Flags: u32 { const A = 1; }`). A same-length rewrite of the parse buffer
//!   turns them into ordinary items at the same offsets; every retained text
//!   (signature, excerpt, documentation) is still read from the real source.
//! * An invocation that looks like it declares items but cannot be parsed
//!   becomes a `macro` declaration named `name!`, so the outline says that
//!   something is declared here which it could not index instead of claiming
//!   a complete file.

use crate::syntax::{SourceLanguage, SyntaxError, node_text};
use std::borrow::Cow;
use tree_sitter::{Language, Node, Parser, Point, Range, Tree};

/// Maximum nested parses one file may cost.
const MAX_NESTED_PARSES: usize = 256;
/// Maximum macro-in-macro depth followed.
const MAX_NESTING_DEPTH: usize = 6;
/// Maximum bytes one macro argument may span before it is left opaque.
const MAX_ARGUMENT_BYTES: usize = 1024 * 1024;

/// Parent kinds whose direct macro invocations declare items.
const ITEM_CONTAINERS: [&str; 6] = [
    "source_file",
    "mod_item",
    "declaration_list",
    "impl_item",
    "trait_item",
    "foreign_mod_item",
];

/// Leaf tokens that begin an item, used to decide that an invocation we could
/// not parse still declares something.
const ITEM_KEYWORDS: [&str; 10] = [
    "fn", "struct", "enum", "union", "trait", "impl", "mod", "type", "const", "static",
];

/// How a macro's argument is rewritten so that it parses as Rust items.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Dialect {
    /// The argument is already written as Rust items.
    Items,
    /// `static ref NAME: T = ..;` declarations.
    LazyStatic,
    /// `struct Flags: T { const A = ..; }` declarations.
    Bitflags,
}

/// One re-parsed macro argument.
pub(crate) struct NestedTree {
    /// The parse of the argument bytes, in the original buffer's coordinates.
    pub(crate) tree: Tree,
    /// Byte range of the invocation this argument belongs to.
    pub(crate) anchor: (usize, usize),
    /// The nested tree holding the invocation, or `None` for the file's tree.
    pub(crate) parent: Option<usize>,
    /// How the argument was rewritten before parsing.
    pub(crate) dialect: Dialect,
}

/// One invocation that declares items this lane could not index.
pub(crate) struct OpaqueInvocation {
    /// Byte range of the invocation.
    pub(crate) range: (usize, usize),
    /// The nested tree holding the invocation, or `None` for the file's tree.
    pub(crate) parent: Option<usize>,
    /// The invoked macro's name as written, without its path.
    pub(crate) name: String,
}

/// Everything one file's item-position macros contributed.
#[derive(Default)]
pub(crate) struct MacroItems {
    pub(crate) nested: Vec<NestedTree>,
    pub(crate) opaque: Vec<OpaqueInvocation>,
}

/// One invocation found in a tree, before it is parsed.
struct Invocation {
    range: (usize, usize),
    name: String,
    ranges: Vec<Range>,
    dialect: Dialect,
    looks_like_items: bool,
}

/// Re-parses every item-position macro argument of a Rust file.
///
/// Languages other than Rust have no item-position macros and contribute
/// nothing.
///
/// # Errors
/// Returns [`SyntaxError::Parser`] when the grammar rejects an included range.
pub(crate) fn macro_items(
    language: SourceLanguage,
    grammar: &Language,
    parser: &mut Parser,
    main: &Tree,
    source: &[u8],
) -> Result<MacroItems, SyntaxError> {
    let mut items = MacroItems::default();
    if language != SourceLanguage::Rust {
        return Ok(items);
    }
    let mut pending = vec![(None, 0usize, invocations(main.root_node(), source))];
    let mut parses = 0usize;
    while let Some((parent, depth, found)) = pending.pop() {
        for invocation in found {
            let parsed = if !invocation.ranges.is_empty()
                && parses < MAX_NESTED_PARSES
                && depth < MAX_NESTING_DEPTH
            {
                parses += 1;
                parse_argument(parser, grammar, &invocation, source)?
            } else {
                None
            };
            match parsed {
                Some(tree) => {
                    let index = items.nested.len();
                    let inner = invocations(tree.root_node(), source);
                    items.nested.push(NestedTree {
                        tree,
                        anchor: invocation.range,
                        parent,
                        dialect: invocation.dialect,
                    });
                    pending.push((Some(index), depth + 1, inner));
                }
                None if invocation.looks_like_items => items.opaque.push(OpaqueInvocation {
                    range: invocation.range,
                    parent,
                    name: invocation.name,
                }),
                None => {}
            }
        }
    }
    // Leave the parser ready for the next file's full parse.
    parser
        .set_included_ranges(&[])
        .map_err(|error| SyntaxError::Parser(error.to_string()))?;
    Ok(items)
}

/// Parses one macro argument as items, or `None` when it is not valid Rust.
fn parse_argument(
    parser: &mut Parser,
    grammar: &Language,
    invocation: &Invocation,
    source: &[u8],
) -> Result<Option<Tree>, SyntaxError> {
    parser
        .set_language(grammar)
        .map_err(|error| SyntaxError::Parser(error.to_string()))?;
    parser
        .set_included_ranges(&invocation.ranges)
        .map_err(|error| SyntaxError::Parser(error.to_string()))?;
    let buffer = rewritten(invocation, source);
    let Some(tree) = parser.parse(buffer.as_ref(), None) else {
        return Ok(None);
    };
    Ok((!tree.root_node().has_error()).then_some(tree))
}

/// Returns the parse buffer: the source itself, or its same-length rewrite.
fn rewritten<'a>(invocation: &Invocation, source: &'a [u8]) -> Cow<'a, [u8]> {
    match invocation.dialect {
        Dialect::Items => Cow::Borrowed(source),
        Dialect::LazyStatic => {
            let mut buffer = source.to_vec();
            blank_lazy_static_refs(&mut buffer, &invocation.ranges);
            Cow::Owned(buffer)
        }
        Dialect::Bitflags => {
            let mut buffer = source.to_vec();
            bitflags_as_enums(&mut buffer, &invocation.ranges);
            Cow::Owned(buffer)
        }
    }
}

/// Finds the item-position invocations of one tree.
fn invocations(root: Node<'_>, source: &[u8]) -> Vec<Invocation> {
    let mut found = Vec::new();
    collect(root, source, &mut found);
    found
}

fn collect(node: Node<'_>, source: &[u8], found: &mut Vec<Invocation>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "macro_invocation" => {
                if let Some(invocation) = invocation_of(child, source) {
                    found.push(invocation);
                }
            }
            kind if ITEM_CONTAINERS.contains(&kind) => collect(child, source, found),
            _ => {}
        }
    }
}

fn invocation_of(node: Node<'_>, source: &[u8]) -> Option<Invocation> {
    let text = std::str::from_utf8(source).ok()?;
    let path = node_text(node.child_by_field_name("macro")?, text);
    let name = path.rsplit("::").next().unwrap_or(path).trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let mut cursor = node.walk();
    let argument = node
        .children(&mut cursor)
        .find(|child| child.kind() == "token_tree")?;
    let dialect = match name.as_str() {
        "lazy_static" => Dialect::LazyStatic,
        "bitflags" => Dialect::Bitflags,
        _ => Dialect::Items,
    };
    let mut ranges: Vec<Range> = if name == "cfg_if" {
        braced_branches(argument)
    } else {
        inner_range(argument).into_iter().collect()
    };
    let bytes = ranges.iter().fold(0usize, |sum, range| {
        sum.saturating_add(range.end_byte.saturating_sub(range.start_byte))
    });
    if bytes > MAX_ARGUMENT_BYTES {
        ranges.clear();
    }
    Some(Invocation {
        range: (node.start_byte(), node.end_byte()),
        name,
        looks_like_items: declares_items(argument, text),
        ranges,
        dialect,
    })
}

/// Returns the bytes between a token tree's delimiters.
fn inner_range(token_tree: Node<'_>) -> Option<Range> {
    let start = token_tree.start_position();
    let end = token_tree.end_position();
    let start_byte = token_tree.start_byte().checked_add(1)?;
    let end_byte = token_tree.end_byte().checked_sub(1)?;
    (start_byte <= end_byte).then_some(Range {
        start_byte,
        end_byte,
        start_point: Point::new(start.row, start.column.saturating_add(1)),
        end_point: Point::new(end.row, end.column.saturating_sub(1)),
    })
}

/// Returns the brace-delimited branches of a `cfg_if!` argument.
fn braced_branches(token_tree: Node<'_>) -> Vec<Range> {
    let mut cursor = token_tree.walk();
    token_tree
        .children(&mut cursor)
        .filter(|child| child.kind() == "token_tree")
        .filter(|child| {
            child
                .child(0)
                .is_some_and(|delimiter| delimiter.kind() == "{")
        })
        .filter_map(inner_range)
        .collect()
}

/// Returns whether an argument holds a keyword that starts an item followed by
/// a name, which is what separates a declaring macro from a computing one.
fn declares_items(argument: Node<'_>, text: &str) -> bool {
    let mut leaves = Vec::new();
    leaf_tokens(argument, &mut leaves, 4_096);
    leaves.windows(2).any(|pair| {
        let keyword = node_text(pair[0], text);
        let next = node_text(pair[1], text);
        ITEM_KEYWORDS.contains(&keyword)
            && next
                .chars()
                .next()
                .is_some_and(|first| first.is_alphabetic() || first == '_')
    })
}

fn leaf_tokens<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>, limit: usize) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if out.len() >= limit {
            return;
        }
        if child.child_count() == 0 {
            out.push(child);
        } else {
            leaf_tokens(child, out, limit);
        }
    }
}

/// Blanks every `ref` in a `static ref` pair so the pair reads as a static.
fn blank_lazy_static_refs(buffer: &mut [u8], ranges: &[Range]) {
    for range in ranges {
        let mut at = range.start_byte;
        while at < range.end_byte {
            let Some(found) = find_word(buffer, at, range.end_byte, b"static") else {
                break;
            };
            let mut next = found + b"static".len();
            while next < range.end_byte && is_space_at(buffer, next) {
                next += 1;
            }
            if word_at(buffer, next, range.end_byte, b"ref") {
                fill(buffer, next, next + b"ref".len(), b' ');
            }
            at = next;
        }
    }
}

fn is_word_byte(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

fn is_space_at(buffer: &[u8], at: usize) -> bool {
    buffer.get(at).is_some_and(u8::is_ascii_whitespace)
}

fn is_word_at(buffer: &[u8], at: usize) -> bool {
    buffer.get(at).is_some_and(|byte| is_word_byte(*byte))
}

/// Overwrites a byte range with one byte; a range outside the buffer is left alone.
fn fill(buffer: &mut [u8], start: usize, end: usize, byte: u8) {
    if let Some(span) = buffer.get_mut(start..end) {
        span.fill(byte);
    }
}

fn word_at(buffer: &[u8], at: usize, end: usize, word: &[u8]) -> bool {
    let stop = at.saturating_add(word.len());
    stop <= end
        && buffer.get(at..stop) == Some(word)
        && !is_word_at(buffer, stop)
        && (at == 0 || !is_word_at(buffer, at - 1))
}

fn find_word(buffer: &[u8], from: usize, end: usize, word: &[u8]) -> Option<usize> {
    (from..end).find(|at| word_at(buffer, *at, end, word))
}

/// Rewrites `struct Flags: T { const A = ..; }` into the same-length enum
/// `enum   Flags    { A,       }`, which is ordinary Rust at the same offsets.
fn bitflags_as_enums(buffer: &mut [u8], ranges: &[Range]) {
    for range in ranges {
        let mut at = range.start_byte;
        while let Some(found) = find_word(buffer, at, range.end_byte, b"struct") {
            at = found + b"struct".len();
            let Some(brace) = struct_body_start(buffer, at, range.end_byte) else {
                continue;
            };
            let Some(close) = matching_brace(buffer, brace, range.end_byte) else {
                continue;
            };
            let Some(colon) = buffer
                .get(at..brace)
                .and_then(|head| head.iter().position(|byte| *byte == b':'))
            else {
                continue;
            };
            if let Some(keyword) = buffer.get_mut(found..at) {
                keyword.copy_from_slice(b"enum  ");
            }
            fill(buffer, at + colon, brace, b' ');
            blank_flag_constants(buffer, brace + 1, close);
            at = close;
        }
    }
}

/// Returns the offset of the `{` that opens a flags struct's body.
fn struct_body_start(buffer: &[u8], from: usize, end: usize) -> Option<usize> {
    buffer
        .get(from..end)?
        .iter()
        .position(|byte| matches!(*byte, b'{' | b';'))
        .map(|offset| from + offset)
        .filter(|at| buffer.get(*at) == Some(&b'{'))
}

fn matching_brace(buffer: &[u8], open: usize, end: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (offset, byte) in buffer.get(open..end)?.iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(open + offset);
                }
            }
            _ => {}
        }
    }
    None
}

/// Turns each `const NAME = value;` at the top of a flags body into `NAME,`.
fn blank_flag_constants(buffer: &mut [u8], from: usize, end: usize) {
    let mut at = from;
    while let Some(found) = find_word(buffer, at, end, b"const") {
        let mut name = found + b"const".len();
        while name < end && is_space_at(buffer, name) {
            name += 1;
        }
        let mut name_end = name;
        while name_end < end && is_word_at(buffer, name_end) {
            name_end += 1;
        }
        let Some(stop) = buffer
            .get(name_end..end)
            .and_then(|rest| rest.iter().position(|byte| *byte == b';'))
            .map(|offset| name_end + offset)
        else {
            break;
        };
        if name_end == name {
            at = name_end;
            continue;
        }
        fill(buffer, found, found + b"const".len(), b' ');
        if let Some(value) = buffer.get_mut(name_end..stop) {
            for byte in value {
                if !byte.is_ascii_whitespace() {
                    *byte = b' ';
                }
            }
        }
        if let Some(terminator) = buffer.get_mut(stop) {
            *terminator = b',';
        }
        at = stop + 1;
    }
}
